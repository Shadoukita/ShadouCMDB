<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useCiClasses, useClassAttributes } from "../../../api/queries";
import {
  useCreateWorkflow,
  useDeleteWorkflow,
  useUpdateWorkflow,
  type WorkflowDefinitionDetail,
  type WorkflowUpdateBody,
  type WorkflowWarning,
} from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import { vAutofocus } from "../../../lib/directives";
import { formatDateTime } from "../../../lib/format";
import { keyError, suggestKey } from "../../../lib/keys";
import { useFlashStore } from "../../../stores/flash";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";
import UninstancedWarning from "./UninstancedWarning.vue";
import { uninstancedText } from "./uninstanced";

/**
 * A workflow's identity and flags: on create the key and CI type too (both never change). Turning
 * on a workflow that drives a state field is confirmed first, because CIs without a running
 * instance cannot have that field edited until instances are started on them; the API's
 * `UNINSTANCED_CIS` warning then says how many.
 */
const props = defineProps<{ workflow?: WorkflowDefinitionDetail }>();
const route = useRoute();
const router = useRouter();
const flash = useFlashStore();
const isNew = computed(() => !props.workflow);
const classes = useCiClasses();
const create = useCreateWorkflow();
const update = useUpdateWorkflow();
const pending = computed(() => create.isPending.value || update.isPending.value);

interface Form {
  key: string;
  name: string;
  description: string;
  classId: string;
  includeSubclasses: boolean;
  stateAttributeId: string;
  autoStart: boolean;
  isActive: boolean;
}
const blank = (): Form => ({
  key: "",
  name: "",
  description: "",
  classId: typeof route.query.classId === "string" ? route.query.classId : "",
  includeSubclasses: true,
  stateAttributeId: "",
  autoStart: false,
  isActive: false,
});
const fromWorkflow = (w: WorkflowDefinitionDetail): Form => ({
  key: w.key,
  name: w.name,
  description: w.description ?? "",
  classId: w.classId,
  includeSubclasses: w.includeSubclasses,
  stateAttributeId: w.stateAttributeId ?? "",
  autoStart: w.autoStart,
  isActive: w.isActive,
});
const base = ref<Form | null>(props.workflow ? fromWorkflow(props.workflow) : null);
const form = ref<Form>(props.workflow ? fromWorkflow(props.workflow) : blank());
const keyTouched = ref(false);
const dirty = computed(() => !!base.value && JSON.stringify(form.value) !== JSON.stringify(base.value));

// Refetched (another tab, our own grants change): take it unless that would overwrite unsaved edits.
watch(
  () => props.workflow,
  (w) => {
    if (w && !dirty.value) {
      base.value = fromWorkflow(w);
      form.value = fromWorkflow(w);
    }
  },
);
watch(
  () => form.value.name,
  (n) => {
    if (isNew.value && !keyTouched.value) form.value.key = suggestKey(n);
  },
);

// The state field: an active lookup field of the type, own or inherited.
const attrs = useClassAttributes(() => form.value.classId || undefined);
const lookupFields = computed(() => (attrs.data.value ?? []).filter((a) => a.dataType === "lookup" && a.isActive));
watch(
  () => form.value.classId,
  (now, before) => {
    if (isNew.value && before !== undefined && now !== before) form.value.stateAttributeId = "";
  },
);
/** The state field can change until a version is published (the API answers 409 CONFLICT `published` after). */
const stateFieldLocked = computed(() => !!props.workflow && props.workflow.currentVersionNo !== null);
const className = computed(() => classes.data.value?.find((c) => c.id === form.value.classId)?.name);

const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
const saved = ref<string | null>(null);
const warnings = ref<WorkflowWarning[]>([]);
const FIELDS = ["key", "name", "description", "classId", "includeSubclasses", "stateAttributeId", "autoStart", "isActive"];
const conflict = computed(() => error.value instanceof ApiError && error.value.code === "VERSION_CONFLICT");
const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field)) : []));

/** Saving this would make the workflow an active driver of a state field: confirm first. */
const activating = computed(() => {
  const f = form.value;
  if (!f.isActive || !f.stateAttributeId) return false;
  const b = base.value;
  return !b || !b.isActive || b.stateAttributeId !== f.stateAttributeId;
});
const confirmActivation = ref(false);

function validate(): boolean {
  const f = form.value;
  const errs: Record<string, string> = {};
  if (!f.name.trim()) errs.name = "Required";
  if (isNew.value) {
    const k = keyError(f.key);
    if (k) errs.key = k;
    if (!f.classId) errs.classId = "Required";
  }
  local.value = errs;
  const first = FIELDS.find((k) => errs[k]);
  if (first) document.getElementById(`wf-${first}`)?.focus();
  return !first;
}

function submit() {
  error.value = null;
  saved.value = null;
  if (!validate()) return;
  if (activating.value) {
    confirmActivation.value = true;
    return;
  }
  void save();
}

async function save() {
  confirmActivation.value = false;
  const f = form.value;
  const description = f.description.trim() || null;
  try {
    if (isNew.value) {
      const created = await create.mutateAsync({
        key: f.key,
        name: f.name.trim(),
        description,
        classId: f.classId,
        includeSubclasses: f.includeSubclasses,
        stateAttributeId: f.stateAttributeId || null,
        autoStart: f.autoStart,
        isActive: f.isActive,
      });
      // The activation warning (a new active workflow with a state field) must not get lost with the navigation.
      const w = created.warnings.find((x) => x.code === "UNINSTANCED_CIS");
      flash.show(created.id, `Workflow ${created.name} created. ${w ? uninstancedText(w) : "Design its states and transitions, then publish it."}`);
      await router.push({ path: `/admin/workflows/${created.id}`, query: { tab: "designer" } });
      return;
    }
    const w = props.workflow!;
    const b = base.value!;
    const body: WorkflowUpdateBody = { version: w.version };
    if (f.name.trim() !== b.name) body.name = f.name.trim();
    if (description !== (b.description || null)) body.description = description;
    if (f.includeSubclasses !== b.includeSubclasses) body.includeSubclasses = f.includeSubclasses;
    if (f.stateAttributeId !== b.stateAttributeId) body.stateAttributeId = f.stateAttributeId || null;
    if (f.autoStart !== b.autoStart) body.autoStart = f.autoStart;
    if (f.isActive !== b.isActive) body.isActive = f.isActive;
    if (Object.keys(body).length === 1) {
      saved.value = "Nothing changed.";
      return;
    }
    const next = await update.mutateAsync({ id: w.id, body });
    base.value = fromWorkflow(next);
    form.value = fromWorkflow(next);
    warnings.value = next.warnings;
    saved.value = `Saved ${next.name}.`;
  } catch (e) {
    error.value = e;
  }
}

function reloadAfterConflict() {
  error.value = null;
  if (props.workflow) {
    base.value = fromWorkflow(props.workflow);
    form.value = fromWorkflow(props.workflow);
  }
}

// ---------- Delete ----------

const del = useDeleteWorkflow();
const deleting = ref(false);
function confirmDelete() {
  const w = props.workflow;
  if (!w) return;
  del.mutate(w.id, {
    onSuccess: () => {
      flash.show("workflows", `Workflow ${w.name} deleted.`);
      void router.replace("/admin/workflows");
    },
  });
}
const deleteInUse = computed(() => del.error.value instanceof ApiError && del.error.value.code === "IN_USE");
</script>

<template>
  <div class="grid-2">
    <form class="panel" aria-labelledby="wf-form-title" novalidate @submit.prevent="submit">
      <div class="panel-header"><h2 id="wf-form-title">Settings</h2></div>
      <div class="panel-body stack">
        <div v-if="conflict" class="alert alert-warn" role="alert">
          <div>Someone changed this workflow (its settings, grants or versions) since you opened it. Your changes were not saved.</div>
          <div><button type="button" class="btn btn-sm" @click="reloadAfterConflict">Load the current settings</button></div>
        </div>
        <FormErrorBanner v-else-if="error" :error="error" :unplaced="unplaced" />
        <div v-if="saved" class="alert" role="status">{{ saved }}</div>
        <UninstancedWarning v-for="w in warnings" :key="w.code" :warning="w" />
        <div class="form-grid">
          <FormField id="wf-name" label="Name" required :error="fieldErrors.name" hint="Shown to operators when they run a transition.">
            <template #default="{ id: fid, invalid, describedBy }">
              <input :id="fid" v-model="form.name" v-autofocus="isNew" type="text" maxlength="100" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
            </template>
          </FormField>
          <FormField
            id="wf-key"
            label="Key"
            :required="isNew"
            :error="fieldErrors.key"
            :hint="isNew ? 'Stable identity for export and the API; it never changes.' : 'Never changes.'"
          >
            <template #default="{ id: fid, invalid, describedBy }">
              <input
                :id="fid"
                v-model="form.key"
                class="mono"
                type="text"
                maxlength="63"
                autocomplete="off"
                spellcheck="false"
                :readonly="!isNew"
                :aria-invalid="invalid"
                :aria-describedby="describedBy"
                @input="keyTouched = true"
              />
            </template>
          </FormField>
          <FormField
            id="wf-classId"
            label="CI type"
            :required="isNew"
            :error="fieldErrors.classId"
            :hint="isNew ? 'The type whose CIs run this workflow. It never changes.' : 'Never changes.'"
          >
            <template #default="{ id: fid, invalid, describedBy }">
              <select v-if="isNew" :id="fid" v-model="form.classId" :aria-invalid="invalid" :aria-describedby="describedBy">
                <option value="" disabled>Choose a type…</option>
                <option v-for="c in classes.data.value ?? []" :key="c.id" :value="c.id" :disabled="!c.isActive">{{ c.name }}</option>
              </select>
              <input v-else :id="fid" type="text" readonly :value="className ?? workflow?.classKey" :aria-describedby="describedBy" />
            </template>
          </FormField>
          <FormField
            id="wf-stateAttributeId"
            label="State field"
            :error="fieldErrors.stateAttributeId"
            :hint="
              stateFieldLocked
                ? 'Fixed once a version has been published.'
                : 'Optional. A dropdown field of the type that the workflow keeps in step with its state; operators can no longer edit it directly while the workflow is active.'
            "
          >
            <template #default="{ id: fid, invalid, describedBy }">
              <select :id="fid" v-model="form.stateAttributeId" :disabled="stateFieldLocked || !form.classId" :aria-invalid="invalid" :aria-describedby="describedBy">
                <option value="">None</option>
                <option v-for="a in lookupFields" :key="a.id" :value="a.id">{{ a.label }} ({{ a.key }})</option>
              </select>
            </template>
          </FormField>
          <FormField id="wf-description" label="Description" :error="fieldErrors.description" wide>
            <template #default="{ id: fid, invalid, describedBy }">
              <textarea :id="fid" v-model="form.description" rows="3" maxlength="4000" :aria-invalid="invalid" :aria-describedby="describedBy" />
            </template>
          </FormField>
        </div>
        <fieldset class="group">
          <legend>Behaviour</legend>
          <label class="checkbox-row"><input id="wf-includeSubclasses" v-model="form.includeSubclasses" type="checkbox" /> CIs of its subtypes run it too</label>
          <label class="checkbox-row"><input id="wf-autoStart" v-model="form.autoStart" type="checkbox" /> Start an instance when a CI of the type is created</label>
          <label class="checkbox-row"><input id="wf-isActive" v-model="form.isActive" type="checkbox" /> Active: new instances can start</label>
          <p class="hint no-margin">An inactive workflow starts no new instances; running ones continue.</p>
        </fieldset>
      </div>
      <div class="form-footer">
        <button type="submit" class="btn btn-primary" :disabled="pending">
          {{ pending ? "Saving…" : isNew ? "Create workflow" : "Save changes" }}
        </button>
        <RouterLink class="btn" to="/admin/workflows">{{ isNew ? "Cancel" : "Back to workflows" }}</RouterLink>
      </div>
    </form>

    <div v-if="workflow" class="stack">
      <section class="panel" aria-labelledby="wf-facts-title">
        <div class="panel-header"><h2 id="wf-facts-title">Facts</h2></div>
        <div class="panel-body">
          <dl class="props">
            <dt>Current version</dt>
            <dd>{{ workflow.currentVersionNo ?? "None published" }}</dd>
            <dt>Draft</dt>
            <dd>{{ workflow.draftVersionNo !== null ? `Version ${workflow.draftVersionNo}` : "None" }}</dd>
            <dt>Created</dt>
            <dd>{{ formatDateTime(workflow.createdAt) }} by {{ workflow.createdByName }}</dd>
            <dt>Updated</dt>
            <dd>{{ formatDateTime(workflow.updatedAt) }} by {{ workflow.updatedByName }}</dd>
          </dl>
        </div>
      </section>
      <section class="panel" aria-labelledby="wf-danger-title">
        <div class="panel-header"><h2 id="wf-danger-title">Delete</h2></div>
        <div class="panel-body stack">
          <p class="muted no-margin">
            Only a workflow that never ran can be deleted, with its versions and grants. Once a CI has had an instance, its history
            keeps the workflow: deactivate it instead.
          </p>
          <div><button type="button" class="btn btn-danger" @click="(del.reset(), (deleting = true))">Delete workflow</button></div>
        </div>
      </section>
    </div>
  </div>

  <ConfirmDialog
    :open="confirmActivation"
    title="Activate a workflow that drives a state field?"
    confirm-label="Activate"
    tone="primary"
    :busy="pending"
    @cancel="confirmActivation = false"
    @confirm="save"
  >
    <p>
      While this workflow is active it owns the state field: operators can no longer edit that field directly on any CI it covers.
    </p>
    <p>
      CIs that already exist have no running instance of it, so their state field stays locked until an instance is started on them.
      Before activating, plan to adopt the existing CIs with the workflow's <strong>bootstrap</strong>, which starts an instance on each
      CI in the state matching its current value. After saving, the number of affected CIs is shown here.
    </p>
  </ConfirmDialog>

  <ConfirmDialog
    v-if="workflow"
    :open="deleting"
    :title="`Delete workflow ${workflow.name}?`"
    confirm-label="Delete workflow"
    :busy="del.isPending.value"
    @cancel="deleting = false"
    @confirm="confirmDelete"
  >
    <div v-if="deleteInUse" class="alert alert-warn" role="alert">
      This workflow has run on at least one CI, so its history keeps it. Deactivate it in Settings instead.
    </div>
    <ErrorAlert v-else-if="del.isError.value" :error="del.error.value" title="The workflow was not deleted" />
    <p>
      This deletes the workflow <strong>{{ workflow.name }}</strong> (<span class="mono">{{ workflow.key }}</span>) with every version,
      its draft and its grants. It cannot be undone.
    </p>
  </ConfirmDialog>
</template>
