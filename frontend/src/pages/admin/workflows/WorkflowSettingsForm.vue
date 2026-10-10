<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useLookupListValues } from "../../../api/datamodel";
import { useCiClasses, useClassAttributes } from "../../../api/queries";
import {
  useCreateWorkflow,
  useDeleteWorkflow,
  useSaveDraft,
  useSaveGrants,
  useUpdateWorkflow,
  type WorkflowBootstrapResult,
  type WorkflowDefinitionDetail,
  type WorkflowUpdateBody,
  type WorkflowWarning,
} from "../../../api/workflows";
import ConfirmDialog from "../../../components/ConfirmDialog.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import SaveBar from "../../../components/SaveBar.vue";
import { t, tAround } from "../../../i18n";
import { vAutofocus } from "../../../lib/directives";
import { formatDateTime } from "../../../lib/format";
import { keyError, suggestKey } from "../../../lib/keys";
import { toDraftBody, type Draft } from "../../../lib/workflowDraft";
import { WORKFLOW_TEMPLATES, templateDraft, templateFromQuery, type WorkflowTemplateKey } from "../../../lib/workflowTemplates";
import { useFlashStore } from "../../../stores/flash";
import { useSessionStore } from "../../../stores/session";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";
import UninstancedWarning from "./UninstancedWarning.vue";
import WorkflowBootstrap from "./WorkflowBootstrap.vue";
import WorkflowTemplatePanel from "./WorkflowTemplatePanel.vue";
import { uninstancedText } from "./uninstanced";

/**
 * A workflow's identity and flags: on create the key and CI type too (both never change), and what it
 * starts from (`?template=`: blank, or a ready-made graph such as the lifecycle). Turning
 * on a workflow that drives a state field is confirmed first, because CIs without a running
 * instance cannot have that field edited until instances are started on them; the API's
 * `UNINSTANCED_CIS` warning then says how many. Saving goes through the shared save bar, with a toast.
 */
const props = defineProps<{ workflow?: WorkflowDefinitionDetail }>();
const route = useRoute();
const router = useRouter();
const flash = useFlashStore();
const session = useSessionStore();
const isNew = computed(() => !props.workflow);
const classes = useCiClasses();
const create = useCreateWorkflow();
const update = useUpdateWorkflow();
const saveDraft = useSaveDraft();
const saveGrants = useSaveGrants();
const del = useDeleteWorkflow();
/** Creating from a template takes several requests; the form stays busy until the last one. */
const building = ref(false);
const pending = computed(() => create.isPending.value || update.isPending.value || building.value);

// ---------- Template (new workflows only) ----------

const template = computed<WorkflowTemplateKey>(() => (isNew.value ? templateFromQuery(route.query.template) : "blank"));
function chooseTemplate(key: WorkflowTemplateKey) {
  void router.replace({ query: { ...route.query, template: key === "blank" ? undefined : key } });
}
const templateProfiles = ref<string[]>([]);

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

/**
 * GH#667: a workflow can only be created on a type the manager may view and edit (with its subtypes when
 * they run it too); the API refuses others with 403. Grants are per exact type, as on the server.
 */
function mayCover(classId: string): boolean {
  const all = classes.data.value ?? [];
  const covered = [classId];
  if (form.value.includeSubclasses) {
    for (let i = 0; i < covered.length; i++) for (const c of all) if (c.parentId === covered[i]) covered.push(c.id);
  }
  return covered.every((id) => session.canOnClass(id, "edit"));
}

// The template's states take the state field's values; the draft is rebuilt as the field changes.
const stateField = computed(() => lookupFields.value.find((a) => a.id === form.value.stateAttributeId));
const stateValuesQ = useLookupListValues(() => (template.value !== "blank" ? stateField.value?.lookupListId : undefined));
const stateValues = computed(() => (form.value.stateAttributeId ? (stateValuesQ.data.value ?? []) : null));
const draft = computed(() => templateDraft(template.value, stateValues.value));

const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
/**
 * The `UNINSTANCED_CIS` banner: from the detail when the API sends it, else from the last save or bootstrap. A
 * bootstrap leaves the CIs it skipped without an instance, so its result replaces the count. Turning the workflow off
 * or dropping its state field ends the warning.
 */
const warnings = ref<WorkflowWarning[]>(props.workflow?.warnings ?? []);
watch(
  () => props.workflow,
  (w) => {
    if (!w) return;
    if (w.warnings.length > 0) warnings.value = w.warnings;
    else if (!w.isActive || !w.stateAttributeId) warnings.value = [];
  },
);
function onBootstrapped(r: WorkflowBootstrapResult) {
  const count = r.skippedTerminal + r.skippedUnmapped;
  warnings.value = [{ code: "UNINSTANCED_CIS", count, message: uninstancedText({ count }) }];
}
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
  if (!f.name.trim()) errs.name = t("common.required");
  if (isNew.value) {
    const k = keyError(f.key);
    if (k) errs.key = k;
    if (!f.classId) errs.classId = t("common.required");
  }
  local.value = errs;
  const first = FIELDS.find((k) => errs[k]);
  if (first) document.getElementById(`wf-${first}`)?.focus();
  return !first;
}

function submit() {
  error.value = null;
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
  // Taken now: the state field's values may refetch while the requests run.
  const graph = isNew.value ? draft.value : null;
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
      if (graph && !(await applyTemplate(created, graph))) return;
      // The activation warning (a new active workflow with a state field) must not get lost with the navigation.
      const w = created.warnings.find((x) => x.code === "UNINSTANCED_CIS");
      const next = graph ? t("wfTemplate.createdNext") : t("wfAdmin.settings.createdNext");
      flash.show(t("wfAdmin.settings.created", { name: created.name, next: w ? uninstancedText(w) : next }));
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
      flash.show(t("common.nothingChanged"));
      return;
    }
    const next = await update.mutateAsync({ id: w.id, body });
    base.value = fromWorkflow(next);
    form.value = fromWorkflow(next);
    // The banner follows the answer through the detail cache (see `warnings`).
    flash.show(t("record.saved", { name: next.name }));
  } catch (e) {
    error.value = e;
  }
}

/**
 * Stores the template's graph as the new workflow's draft and grants its transitions to the chosen profiles,
 * through the same endpoints the designer and the Grants tab use. The API has no single request for all of
 * it, so when one fails the workflow just created is deleted again (it never ran): no half-built workflow
 * is left behind. False, with the error shown, when the template could not be applied.
 */
async function applyTemplate(created: WorkflowDefinitionDetail, graph: Draft): Promise<boolean> {
  building.value = true;
  try {
    await saveDraft.mutateAsync({ id: created.id, body: toDraftBody(graph) });
    if (templateProfiles.value.length > 0) {
      const profiles = templateProfiles.value;
      await saveGrants.mutateAsync({
        id: created.id,
        body: { version: created.version, grants: graph.transitions.map((x) => ({ transitionKey: x.key, profiles })) },
      });
    }
    return true;
  } catch (e) {
    error.value = e;
    try {
      await del.mutateAsync(created.id);
      flash.show(t("wfTemplate.rolledBack", { name: created.name }));
    } catch {
      // The workflow stays: say so, and where to finish or delete it.
      orphan.value = created;
    }
    return false;
  } finally {
    building.value = false;
  }
}
/** A workflow created from a template whose graph could not be stored, and that could not be deleted again. */
const orphan = ref<WorkflowDefinitionDetail | null>(null);

function discard() {
  error.value = null;
  local.value = {};
  if (base.value) form.value = { ...base.value };
}
const changes = computed(() => (base.value ? (Object.keys(form.value) as (keyof Form)[]).filter((k) => form.value[k] !== base.value![k]).length : 0));

function reloadAfterConflict() {
  error.value = null;
  if (props.workflow) {
    base.value = fromWorkflow(props.workflow);
    form.value = fromWorkflow(props.workflow);
  }
}

// ---------- Delete ----------

const deleting = ref(false);
function confirmDelete() {
  const w = props.workflow;
  if (!w) return;
  del.mutate(w.id, {
    onSuccess: () => {
      flash.show(t("wfAdmin.settings.deleted", { name: w.name }));
      void router.replace("/admin/workflows");
    },
  });
}
const deleteInUse = computed(() => del.error.value instanceof ApiError && del.error.value.code === "IN_USE");
const deleteParts = computed(() => tAround("wfAdmin.delete.body", "name"));
</script>

<template>
  <div class="grid-2">
    <form id="wf-settings-form" class="panel" aria-labelledby="wf-form-title" novalidate @submit.prevent="submit">
      <div class="panel-header"><h2 id="wf-form-title">{{ t("wfAdmin.tab.settings") }}</h2></div>
      <div class="panel-body stack">
        <div v-if="conflict" class="alert alert-warn" role="alert">
          <div>{{ t("wfAdmin.settings.conflict") }}</div>
          <div><button type="button" class="btn btn-sm" @click="reloadAfterConflict">{{ t("wfAdmin.settings.reload") }}</button></div>
        </div>
        <FormErrorBanner v-else-if="error" :error="error" :unplaced="unplaced" />
        <div v-if="orphan" class="alert alert-warn" role="alert" data-testid="wf-template-orphan">
          <div>{{ t("wfTemplate.orphan", { name: orphan.name }) }}</div>
          <div><RouterLink class="btn btn-sm" :to="`/admin/workflows/${orphan.id}`">{{ t("wfTemplate.orphanOpen") }}</RouterLink></div>
        </div>
        <fieldset v-if="isNew" class="group" data-testid="wf-template-choice">
          <legend>{{ t("wfTemplate.legend") }}</legend>
          <label v-for="tpl in WORKFLOW_TEMPLATES" :key="tpl.key" class="checkbox-row">
            <input type="radio" name="wf-template" :value="tpl.key" :checked="template === tpl.key" @change="chooseTemplate(tpl.key)" />
            <span>
              {{ t(tpl.label) }}
              <span class="hint">{{ t(tpl.hint) }}</span>
            </span>
          </label>
        </fieldset>
        <UninstancedWarning v-for="w in warnings" :key="w.code" :warning="w" :bootstrap-target="workflow?.stateAttributeId ? 'wf-bootstrap' : undefined" />
        <div class="form-grid">
          <FormField id="wf-name" :label="t('wfAdmin.field.name')" required :error="fieldErrors.name" :hint="t('wfAdmin.field.nameHint')">
            <template #default="{ id: fid, invalid, describedBy }">
              <input :id="fid" v-model="form.name" v-autofocus="isNew" type="text" maxlength="100" autocomplete="off" :aria-invalid="invalid" :aria-describedby="describedBy" />
            </template>
          </FormField>
          <FormField
            id="wf-key"
            :label="t('wfAdmin.field.key')"
            :required="isNew"
            :error="fieldErrors.key"
            :hint="isNew ? t('wfAdmin.field.keyHint') : t('wfAdmin.field.fixed')"
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
            :label="t('wfAdmin.col.class')"
            :required="isNew"
            :error="fieldErrors.classId"
            :hint="isNew ? t('wfAdmin.field.classHint') : t('wfAdmin.field.fixed')"
          >
            <template #default="{ id: fid, invalid, describedBy }">
              <select v-if="isNew" :id="fid" v-model="form.classId" :aria-invalid="invalid" :aria-describedby="describedBy">
                <option value="" disabled>{{ t("wfAdmin.field.classChoose") }}</option>
                <option v-for="c in classes.data.value ?? []" :key="c.id" :value="c.id" :disabled="!c.isActive || !mayCover(c.id)">{{ c.name }}</option>
              </select>
              <input v-else :id="fid" type="text" readonly :value="className ?? workflow?.classKey" :aria-describedby="describedBy" />
            </template>
          </FormField>
          <FormField
            id="wf-stateAttributeId"
            :label="t('wfAdmin.col.stateField')"
            :error="fieldErrors.stateAttributeId"
            :hint="stateFieldLocked ? t('wfAdmin.field.stateLocked') : t('wfAdmin.field.stateHint')"
          >
            <template #default="{ id: fid, invalid, describedBy }">
              <select :id="fid" v-model="form.stateAttributeId" :disabled="stateFieldLocked || !form.classId" :aria-invalid="invalid" :aria-describedby="describedBy">
                <option value="">{{ t("wfAdmin.none") }}</option>
                <option v-for="a in lookupFields" :key="a.id" :value="a.id">{{ a.label }} ({{ a.key }})</option>
              </select>
            </template>
          </FormField>
          <FormField id="wf-description" :label="t('wfAdmin.field.description')" :error="fieldErrors.description" wide>
            <template #default="{ id: fid, invalid, describedBy }">
              <textarea :id="fid" v-model="form.description" rows="3" maxlength="4000" :aria-invalid="invalid" :aria-describedby="describedBy" />
            </template>
          </FormField>
        </div>
        <fieldset class="group">
          <legend>{{ t("wfAdmin.field.behaviour") }}</legend>
          <label class="checkbox-row"><input id="wf-includeSubclasses" v-model="form.includeSubclasses" type="checkbox" /> {{ t("wfAdmin.field.includeSubclasses") }}</label>
          <label class="checkbox-row"><input id="wf-autoStart" v-model="form.autoStart" type="checkbox" /> {{ t("wfAdmin.field.autoStart") }}</label>
          <label class="checkbox-row"><input id="wf-isActive" v-model="form.isActive" type="checkbox" /> {{ t("wfAdmin.field.isActive") }}</label>
          <p class="hint no-margin">{{ t("wfAdmin.field.isActiveHint") }}</p>
        </fieldset>
      </div>
    </form>

    <WorkflowTemplatePanel v-if="isNew && draft" v-model:profiles="templateProfiles" :draft="draft" :state-values="stateValues" />
    <div v-if="workflow" class="stack">
      <section class="panel" aria-labelledby="wf-facts-title">
        <div class="panel-header"><h2 id="wf-facts-title">{{ t("wfAdmin.facts.title") }}</h2></div>
        <div class="panel-body">
          <dl class="props">
            <dt>{{ t("wfAdmin.col.version") }}</dt>
            <dd>{{ workflow.currentVersionNo ?? t("wfAdmin.facts.nonePublished") }}</dd>
            <dt>{{ t("wfAdmin.col.draft") }}</dt>
            <dd>{{ workflow.draftVersionNo !== null ? t("wfAdmin.versionN", { n: workflow.draftVersionNo }) : t("wfAdmin.none") }}</dd>
            <dt>{{ t("common.created") }}</dt>
            <dd>{{ t("wfAdmin.facts.by", { when: formatDateTime(workflow.createdAt), name: workflow.createdByName }) }}</dd>
            <dt>{{ t("common.updated") }}</dt>
            <dd>{{ t("wfAdmin.facts.by", { when: formatDateTime(workflow.updatedAt), name: workflow.updatedByName }) }}</dd>
          </dl>
        </div>
      </section>
      <WorkflowBootstrap v-if="workflow.stateAttributeId" :workflow="workflow" @done="onBootstrapped" />
      <section class="panel" aria-labelledby="wf-danger-title">
        <div class="panel-header"><h2 id="wf-danger-title">{{ t("common.delete") }}</h2></div>
        <div class="panel-body stack">
          <p class="muted no-margin">{{ t("wfAdmin.delete.intro") }}</p>
          <div><button type="button" class="btn btn-danger" @click="(del.reset(), (deleting = true))">{{ t("wfAdmin.delete.open") }}</button></div>
        </div>
      </section>
    </div>
  </div>

  <SaveBar :label="t('record.save.region')" :dirty="!isNew && dirty" :changes="isNew ? 0 : changes">
    <RouterLink class="btn" to="/admin/workflows">{{ isNew ? t("common.cancel") : t("wfAdmin.back") }}</RouterLink>
    <button v-if="!isNew && dirty" type="button" class="btn" :disabled="pending" @click="discard">{{ t("record.save.discard") }}</button>
    <button type="submit" form="wf-settings-form" class="btn btn-primary" :disabled="pending">
      {{ pending ? t("common.saving") : isNew ? t("wfAdmin.create") : t("common.saveChanges") }}
    </button>
  </SaveBar>

  <ConfirmDialog
    :open="confirmActivation"
    :title="t('wfAdmin.activate.title')"
    :confirm-label="t('wfAdmin.activate.confirm')"
    tone="primary"
    :busy="pending"
    @cancel="confirmActivation = false"
    @confirm="save"
  >
    <p>{{ t("wfAdmin.activate.owns") }}</p>
    <p>{{ t("wfAdmin.activate.existing") }}</p>
  </ConfirmDialog>

  <ConfirmDialog
    v-if="workflow"
    :open="deleting"
    :title="t('wfAdmin.delete.title', { name: workflow.name })"
    :confirm-label="t('wfAdmin.delete.open')"
    :busy="del.isPending.value"
    @cancel="deleting = false"
    @confirm="confirmDelete"
  >
    <div v-if="deleteInUse" class="alert alert-warn" role="alert">{{ t("wfAdmin.delete.inUse") }}</div>
    <ErrorAlert v-else-if="del.isError.value" :error="del.error.value" :title="t('wfAdmin.delete.failed')" />
    <p>
      {{ deleteParts[0] }}<strong dir="auto">{{ workflow.name }}</strong> (<span class="mono">{{ workflow.key }}</span>){{ deleteParts[1] }}
    </p>
  </ConfirmDialog>
</template>
