<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { asApiError } from "../../../lib/errors";
import type { CiSummary } from "../../../api/queries";
import { useTestAction, type WorkflowActionTestResult } from "../../../api/workflows";
import CiPicker from "../../../components/CiPicker.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { t } from "../../../i18n";
import type { ActionKind } from "../../../lib/workflowActions";

/**
 * The designer's test send (SHAA-2725 §11.1): one notification of a stored action, now, to the admin only.
 * An e-mail goes to the admin's own address, a webhook gets one signed `ping`, an inbox action writes one
 * entry to the admin's notifications. Never to the action's recipients, never through the outbox. A relay
 * or receiver that refuses is a result, shown with its reason; e-mail or webhooks being off on the server
 * is explained in place, since only the operator can change it.
 */
const props = defineProps<{
  workflowId: string;
  classId: string;
  /** The stored actions, of every kind. */
  actions: { key: string; name: string; kind: ActionKind; endpoint?: string | null }[];
  /** Unsaved changes: the test sends the stored action. */
  dirty: boolean;
  /** The action to test first, e.g. the one being edited. */
  selectedKey?: string;
}>();

const key = ref(
  props.selectedKey && props.actions.some((a) => a.key === props.selectedKey) ? props.selectedKey : (props.actions[0]?.key ?? ""),
);
watch(
  () => [props.actions, props.selectedKey] as const,
  ([list, sel]) => {
    if (sel && list.some((a) => a.key === sel)) key.value = sel;
    else if (!list.some((a) => a.key === key.value)) key.value = list[0]?.key ?? "";
  },
);
const selected = computed(() => props.actions.find((a) => a.key === key.value) ?? null);
const ci = ref<{ id: string; name: string } | null>(null);
function pickCi(c: CiSummary | null) {
  ci.value = c ? { id: c.id, name: c.label } : null;
}

const send = useTestAction();
const result = ref<WorkflowActionTestResult | null>(null);
const error = ref<unknown>(null);
/** Always in the DOM, so the outcome of every send is announced (see ActionPreview). */
const announcement = ref("");
// A result belongs to the action it was sent for.
watch(key, () => {
  result.value = null;
  error.value = null;
});

/** Refusals only the operator can lift: explained in place rather than as a failed request. */
const serverOff = computed(() => {
  const code = asApiError(error.value)?.code;
  return code === "MAIL_NOT_CONFIGURED" || code === "WEBHOOKS_DISABLED" ? code : null;
});

async function run() {
  const a = selected.value;
  if (!a || send.isPending.value) return;
  result.value = null;
  error.value = null;
  announcement.value = "";
  try {
    const res = await send.mutateAsync({ id: props.workflowId, key: a.key, ciId: a.kind === "email" ? ci.value?.id : undefined });
    result.value = res;
    announcement.value = res.ok ? t("wfActions.test.ok") : t("wfActions.test.notOk");
  } catch (e) {
    error.value = e;
    announcement.value = t("wfActions.test.failed");
  }
}

/** Why it did not arrive, in the result's own terms: the reason code and the SMTP or HTTP status. */
function detail(r: WorkflowActionTestResult): string {
  return [r.reason, r.statusCode != null ? String(r.statusCode) : null].filter(Boolean).join(" · ");
}
</script>

<template>
  <section class="panel" aria-labelledby="wf-action-test-title" data-testid="wf-action-test">
    <div class="panel-header"><h2 id="wf-action-test-title">{{ t("wfActions.test.title") }}</h2></div>
    <div class="panel-body stack">
      <p class="muted no-margin">{{ t("wfActions.test.intro") }}</p>
      <div v-if="dirty" class="alert alert-warn" role="note">{{ t("wfActions.test.unsaved") }}</div>
      <p v-if="actions.length === 0" class="muted no-margin">{{ t("wfActions.test.none") }}</p>
      <form v-else class="wf-preview-form" @submit.prevent="run">
        <div class="field wf-test-action">
          <label for="wf-action-test-action">{{ t("wfActions.preview.action") }}</label>
          <select id="wf-action-test-action" v-model="key" aria-describedby="wf-action-test-where">
            <option v-for="a in actions" :key="a.key" :value="a.key">{{ a.name }} ({{ a.key }}) · {{ t(`wfActions.kind.${a.kind}`) }}</option>
          </select>
          <span v-if="selected" id="wf-action-test-where" class="hint" data-testid="wf-action-test-where">
            {{ selected.kind === "webhook" ? t("wfActions.test.where.webhook", { key: selected.endpoint ?? "—" }) : t(`wfActions.test.where.${selected.kind}`) }}
          </span>
        </div>
        <div v-if="selected?.kind === 'email'" class="field">
          <label for="wf-action-test-ci">{{ t("wfPreview.ci") }}</label>
          <CiPicker
            id="wf-action-test-ci"
            :class-id="classId"
            :selected="ci"
            :placeholder="t('wfPreview.ciPlaceholder')"
            described-by="wf-action-test-ci-hint"
            @select="pickCi"
          />
          <span id="wf-action-test-ci-hint" class="hint">{{ t("wfActions.test.ciHint") }}</span>
        </div>
        <div class="inline-actions wf-preview-actions">
          <button type="submit" class="btn btn-sm" :disabled="!selected || send.isPending.value" data-testid="wf-action-test-run">
            {{ send.isPending.value ? t("wfActions.test.sending") : t("wfActions.test.run") }}
          </button>
        </div>
      </form>

      <p class="sr-only" role="status" aria-live="polite" data-testid="wf-action-test-status">{{ announcement }}</p>
      <div v-if="serverOff" class="alert alert-warn" role="alert" data-testid="wf-action-test-off">
        <strong>{{ t(`wfActions.test.off.${serverOff}.title`) }}</strong>
        <div>{{ t(`wfActions.test.off.${serverOff}.body`) }}</div>
      </div>
      <ErrorAlert v-else-if="error" :error="error" :title="t('wfActions.test.failed')" />
      <div v-else-if="result" :class="['alert', result.ok ? 'alert-success' : 'alert-error']" data-testid="wf-action-test-result">
        <strong>{{ result.ok ? t("wfActions.test.ok") : t("wfActions.test.notOk") }}</strong>
        <div>{{ result.message }}</div>
        <div class="meta">
          {{ t("wfActions.test.to") }} <span class="mono">{{ result.to }}</span> · {{ t("wfActions.test.duration", { ms: result.durationMs }) }}
          <template v-if="!result.ok && detail(result)"> · <span class="mono">{{ detail(result) }}</span></template>
        </div>
      </div>
    </div>
  </section>
</template>
