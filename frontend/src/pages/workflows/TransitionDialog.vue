<script setup lang="ts">
import { computed, reactive, ref, watch } from "vue";
import { ApiError } from "../../api/client";
import { useClassAttributes, type Ci } from "../../api/queries";
import { useRunTransition, type WorkflowAvailableTransition, type WorkflowInstance, type WorkflowTransitionBody } from "../../api/workflowRuntime";
import AttributeInput from "../../components/AttributeInput.vue";
import ErrorAlert from "../../components/ErrorAlert.vue";
import FormDialog from "../../components/FormDialog.vue";
import Icon from "../../components/Icon.vue";
import { toApiValue, toFormValue, type AttributeShape } from "../../lib/attributeValues";
import { t } from "../../i18n";
import { useFlashStore } from "../../stores/flash";
import FormField from "../form/FormField.vue";
import WorkflowStateBadge from "./WorkflowStateBadge.vue";

/**
 * Runs one transition of an instance: its fields with the inputs of the CI form (the CI's values filled in),
 * a comment (required when the transition says so), and the conditions that block it as the CI stands. A
 * blocked transition is still offered: a value entered here may meet its condition, and the API decides.
 *
 * The API answers 422 WORKFLOW_CONDITION_FAILED (a missing field or comment, a failing condition) and 400
 * VALIDATION_ERROR with `fields.<key>` or `comment`: those messages go next to their inputs. 409
 * VERSION_CONFLICT means someone moved the instance on: the operator reloads it (`reload`).
 *
 * A transition with an approval policy (`requiresApproval`) is checked the same way but only requests the
 * change (202 with `pendingApproval`): the dialog says so and lists the steps before the operator submits.
 */
const props = defineProps<{
  open: boolean;
  instance: WorkflowInstance;
  transition: WorkflowAvailableTransition | null;
  /** The CI's type, for the full field definitions (lists, references). */
  classId?: string;
  /** The CI, for the names of its referenced CIs. */
  ci?: Ci;
}>();
const emit = defineEmits<{ close: []; reload: [] }>();

const flash = useFlashStore();
const run = useRunTransition();
const attrs = useClassAttributes(() => (props.open ? props.classId : undefined));
const values = reactive<Record<string, string>>({});
const initial = reactive<Record<string, string>>({});
const refNames = reactive<Record<string, string>>({});
const comment = ref("");
const error = ref<unknown>(null);

const tr = computed(() => props.transition);
const fieldId = (key: string) => `wf-field-${key}`;
/** The class's definition of a transition field, or the field's own data type when the definition is not loaded. */
function defFor(key: string, dataType: string): AttributeShape {
  const d = attrs.data.value?.find((a) => a.key === key);
  return d ?? ({ dataType, enumValues: [], validation: {}, referenceClassId: null, lookupListId: null } as unknown as AttributeShape);
}

watch(
  () => [props.open, tr.value?.key] as const,
  ([open]) => {
    if (!open || !tr.value) return;
    for (const k of Object.keys(values)) delete values[k];
    for (const f of tr.value.fields) {
      values[f.key] = toFormValue({ dataType: f.dataType }, f.currentValue);
      initial[f.key] = values[f.key];
      const r = props.ci?.attributeReferences[f.key];
      if (r?.name) refNames[f.key] = r.name;
    }
    comment.value = "";
    error.value = null;
    run.reset();
  },
  { immediate: true },
);

const apiError = computed(() => (error.value instanceof ApiError ? error.value : null));
const conflict = computed(() => apiError.value?.code === "VERSION_CONFLICT");
/** Messages by `fields.<key>` / `comment`; the rest is listed above the form. */
const fieldErrors = computed<Record<string, string>>(() => {
  const e = apiError.value;
  if (!e || !["WORKFLOW_CONDITION_FAILED", "VALIDATION_ERROR"].includes(e.code)) return {};
  return e.fieldErrors();
});
const placed = computed(() => new Set([...(tr.value?.fields ?? []).map((f) => `fields.${f.key}`), "comment"]));
const unplaced = computed(() => (apiError.value?.details ?? []).filter((d) => !placed.value.has(d.field)));
const commentError = computed(() => fieldErrors.value.comment);

async function submit() {
  const cur = tr.value;
  if (!cur) return;
  error.value = null;
  // Only the fields the operator changed: the others keep the CI's values, which the API reads itself.
  const fields: Record<string, unknown> = {};
  for (const f of cur.fields) {
    if (values[f.key] === initial[f.key]) continue;
    fields[f.key] = values[f.key] === "" ? null : toApiValue(defFor(f.key, f.dataType), values[f.key]);
  }
  try {
    const after = await run.mutateAsync({
      id: props.instance.id,
      ciId: props.instance.ciId,
      // The spec gives `fields` as an object without properties (`Record<string, never>`); the API takes any field key.
      body: { transitionKey: cur.key, expectedVersion: props.instance.version, fields: fields as WorkflowTransitionBody["fields"], comment: comment.value.trim() || undefined },
    });
    if (after.pendingApproval) flash.show(t("approvalRun.requested", { transition: cur.name, ci: props.instance.ciLabel }));
    else flash.show(t("wfRun.transition.done", { transition: cur.name, ci: props.instance.ciLabel, state: cur.toState.name }));
    emit("close");
  } catch (e) {
    error.value = e;
  }
}
</script>

<template>
  <FormDialog
    :open="open && !!tr"
    :title="tr ? t('wfRun.transition.title', { transition: tr.name, workflow: instance.definitionName }) : ''"
    :submit-label="tr?.requiresApproval ? t('approvalRun.requestSubmit') : (tr?.name ?? t('wfRun.transition.run'))"
    :busy="run.isPending.value"
    wide
    @submit="submit"
    @cancel="emit('close')"
  >
    <template v-if="tr">
      <p class="wf-transition-route">
        <WorkflowStateBadge :state="instance.state" />
        <Icon name="arrow-right" :size="14" aria-hidden="true" /><span class="sr-only">{{ t("wfRun.transition.srTo") }}</span>
        <WorkflowStateBadge :state="tr.toState" />
        <span class="muted" dir="auto">{{ t("wfRun.transition.onCi", { ci: instance.ciLabel, ident: instance.ciIdent }) }}</span>
      </p>
      <div v-if="tr.requiresApproval" class="alert" data-testid="wf-requires-approval">
        <strong>{{ t("approvalRun.requires.title") }}</strong>
        <div>{{ t("approvalRun.requires.body") }}</div>
        <ol class="wf-approval-steps" :aria-label="t('approvalRun.requires.steps')">
          <li v-for="s in tr.approvalSteps" :key="s.key">
            <span dir="auto">{{ s.name }}</span>: {{ t("approvalRun.requires.step", { n: s.requiredApprovals }) }}
          </li>
        </ol>
        <div>{{ t("approvalRun.requires.self") }}</div>
      </div>
      <div v-if="conflict" class="alert alert-warn" role="alert" data-testid="wf-conflict">
        <strong>{{ t("wfRun.conflict.title") }}</strong>
        <div>
          {{ t("wfRun.conflict.body") }}
          <button type="button" class="btn btn-sm" @click="emit('reload')">{{ t("wfRun.conflict.reload") }}</button>
        </div>
      </div>
      <div v-else-if="apiError && (apiError.code === 'WORKFLOW_CONDITION_FAILED' || apiError.code === 'VALIDATION_ERROR')" class="alert alert-error" role="alert">
        <strong>{{ apiError.code === "WORKFLOW_CONDITION_FAILED" ? t("wfRun.transition.conditionsFailed") : t("wfRun.transition.checkValues") }}</strong>
        <ul v-if="unplaced.length > 0">
          <li v-for="(d, i) in unplaced" :key="i">{{ d.message }}</li>
        </ul>
        <div v-else>{{ t("wfRun.transition.seeFields") }}</div>
      </div>
      <ErrorAlert v-else-if="error" :error="error" :title="t('wfRun.transition.failed')" />
      <div v-if="tr.blockedBy.length > 0 && !apiError" class="alert alert-warn" data-testid="wf-blocked">
        <strong>{{ t("wfRun.transition.blocked") }}</strong>
        <ul>
          <li v-for="(b, i) in tr.blockedBy" :key="i">{{ b.message }}</li>
        </ul>
        <div v-if="tr.fields.length > 0">{{ t("wfRun.transition.mayMeet") }}</div>
      </div>
      <div class="form-grid">
        <FormField
          v-for="f in tr.fields"
          :id="fieldId(f.key)"
          :key="f.key"
          v-slot="p"
          :label="f.label"
          :required="f.required"
          :error="fieldErrors[`fields.${f.key}`]"
        >
          <AttributeInput
            :id="p.id"
            v-model="values[f.key]"
            :def="defFor(f.key, f.dataType)"
            :invalid="p.invalid"
            :described-by="p.describedBy"
            :reference-name="refNames[f.key]"
            @reference-name="(n) => (refNames[f.key] = n)"
          />
        </FormField>
        <FormField
          id="wf-comment"
          v-slot="p"
          :label="t('wfRun.comment')"
          :required="tr.requiresComment"
          :error="commentError"
          wide
          :hint="tr.requiresComment ? t('wfRun.transition.commentRequired') : t('wfRun.commentOptional')"
        >
          <textarea :id="p.id" v-model="comment" rows="3" maxlength="4000" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
      </div>
    </template>
  </FormDialog>
</template>
