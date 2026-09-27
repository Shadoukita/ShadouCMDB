<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useAreas, useCiClass, useCreateClass, usePatch, useRemove, type ClassCreateBody, type ClassUpdateBody } from "../../../api/datamodel";
import { usePurge } from "../../../api/schemaChanges";
import { useCiClasses, type CiClass } from "../../../api/queries";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ClassBadge from "../../../components/ClassBadge.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import TechnicalNameField from "../../../components/TechnicalNameField.vue";
import { CLASS_ICONS, classIcon } from "../../../lib/classIcons";
import { useDocumentTitle } from "../../../lib/composables";
import { vAutofocus } from "../../../lib/directives";
import { formatDateTime } from "../../../lib/format";
import { keyError } from "../../../lib/keys";
import { useSchemaChangeFlow } from "../../../lib/schemaChange";
import { descendantIds, flattenTree } from "../../../lib/tree";
import { useFlashStore } from "../../../stores/flash";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";
import AttributesEditor from "./AttributesEditor.vue";

/**
 * Create or edit a CI class (a type): name, area and technical name (both fixed
 * after creation: they place the class's table, e.g. bestand.netzwerk), parent,
 * abstract, icon and colour; archive, restore or purge it. Every change is
 * previewed as DDL first. Below the form, the class's attribute editor
 * (existing classes only).
 */
const route = useRoute();
const router = useRouter();
const flash = useFlashStore();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const cls = useCiClass(id);
const classes = useCiClasses();
const areas = useAreas();
const create = useCreateClass();
const update = usePatch<CiClass>("ci-classes");
const remove = useRemove("ci-classes");
const purge = usePurge("ci-classes");
const flow = useSchemaChangeFlow();
const pending = computed(() => create.isPending.value || update.isPending.value || remove.isPending.value || flow.state.loading);
const flashText = computed(() => (id.value ? flash.forCi(id.value) : undefined));
useDocumentTitle(() => (isNew.value ? "New class" : cls.data.value?.name));

const name = ref("");
const key = ref("");
const areaId = ref("");
/** The administrator picked the area; until then it follows the parent class. */
const areaTouched = ref(false);
const description = ref("");
const parentId = ref("");
const isAbstract = ref(false);
const icon = ref("");
const color = ref("");
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});
const saved = ref<string | null>(null);

function seed(c: CiClass | undefined) {
  name.value = c?.name ?? "";
  key.value = c?.key ?? "";
  areaTouched.value = !!c || typeof route.query.areaId === "string";
  areaId.value = c?.areaId ?? (typeof route.query.areaId === "string" ? route.query.areaId : "");
  description.value = c?.description ?? "";
  parentId.value = c?.parentId ?? (typeof route.query.parentId === "string" ? route.query.parentId : "");
  isAbstract.value = c?.isAbstract ?? false;
  icon.value = c?.icon ?? "";
  color.value = c?.color ?? "";
}
watch(() => cls.data.value, seed, { immediate: true });
watch(id, () => {
  if (!id.value) seed(undefined);
  error.value = null;
  local.value = {};
  saved.value = null;
});
const activeAreas = computed(() => (areas.data.value ?? []).filter((a) => a.isActive));
const area = computed(() => areas.data.value?.find((a) => a.id === (cls.data.value?.areaId ?? areaId.value)));
// A new class goes into its parent's area unless the administrator chose another; a root class into the first area.
watch(
  () => [parentId.value, classes.data.value, areas.data.value] as const,
  () => {
    if (!isNew.value || areaTouched.value) return;
    const parent = classes.data.value?.find((c) => c.id === parentId.value);
    areaId.value = parent?.areaId ?? activeAreas.value[0]?.id ?? "";
  },
  { immediate: true },
);

/** Parent choices: every class except this one and its subclasses (that would be a cycle), indented as a tree. */
const parentOptions = computed(() => {
  const list = classes.data.value ?? [];
  const excluded = id.value ? new Set([id.value, ...descendantIds(list, id.value)]) : new Set<string>();
  return flattenTree(list).filter((n) => !excluded.has(n.item.id));
});
const unknownIcon = computed(() => !!icon.value && !classIcon(icon.value));

const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const FIELDS = ["name", "key", "areaId", "description", "parentId", "isAbstract", "icon", "color"];
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field)) : []));

async function submit() {
  error.value = null;
  saved.value = null;
  const errs: Record<string, string> = {};
  if (!name.value.trim()) errs.name = "Required";
  if (isNew.value) {
    const k = keyError(key.value);
    if (k) errs.key = k;
    if (!areaId.value) errs.areaId = "Choose the area its table goes into";
  }
  local.value = errs;
  if (Object.keys(errs).length > 0) {
    document.getElementById(errs.name ? "class-name" : errs.key ? "class-key" : "class-area")?.focus();
    return;
  }
  const body: ClassUpdateBody = {
    name: name.value.trim(),
    description: description.value.trim() || null,
    parentId: parentId.value || null,
    isAbstract: isAbstract.value,
    icon: icon.value || null,
    color: color.value || null,
  };
  if (isNew.value) {
    // New classes go to the end of the menu.
    const last = Math.max(0, ...(classes.data.value ?? []).map((c) => c.sortOrder));
    const createBody: ClassCreateBody = { ...body, name: body.name!, key: key.value, areaId: areaId.value, sortOrder: last + 10 };
    const where = `${area.value?.key ?? "?"}.${key.value}`;
    const outcome = await flow.run({
      title: `Create class “${createBody.name}”`,
      intro: body.isAbstract
        ? `An abstract class holds no CIs of its own, but its attributes are columns of the table ${where}, which its subclasses' CIs fill.`
        : `Its CIs are stored in the new table ${where}, one typed column per attribute.`,
      preview: { operation: "createType", body: createBody },
      apply: () => create.mutateAsync(createBody),
      applyLabel: "Create class",
      alwaysShow: true,
    });
    if (outcome.status === "applied") {
      const created = outcome.result as CiClass;
      flash.show(created.id, `Created class ${created.name} (table ${created.tableName}). Add its attributes below.`);
      await router.push(`/admin/classes/${created.id}`);
    } else if (outcome.status === "refused") error.value = outcome.error;
    return;
  }
  const outcome = await flow.run({
    title: `Save class “${body.name}”`,
    preview: { operation: "updateType", id: id.value!, body },
    apply: () => update.mutateAsync({ id: id.value!, body }),
    applyLabel: "Save class",
  });
  if (outcome.status === "applied") saved.value = `Saved ${(outcome.result as CiClass).name}.`;
  else if (outcome.status === "refused") error.value = outcome.error;
}

async function setActive(isActive: boolean) {
  const c = cls.data.value!;
  error.value = null;
  saved.value = null;
  const outcome = isActive
    ? await flow.run({
        title: `Restore class “${c.name}”`,
        preview: { operation: "updateType", id: c.id, body: { isActive: true } },
        apply: () => update.mutateAsync({ id: c.id, body: { isActive: true } }),
        applyLabel: "Restore class",
      })
    : await flow.run({
        title: `Archive class “${c.name}”?`,
        intro: `Its table ${c.tableName}, its CIs and every stored value are kept and stay readable, but no new CIs can be created and the menu hides it. Restore it at any time; only a purge deletes the data.`,
        preview: { operation: "deleteType", id: c.id },
        apply: () => remove.mutateAsync(c.id),
        applyLabel: "Archive class",
        alwaysShow: true,
      });
  if (outcome.status === "applied")
    saved.value = isActive
      ? `Restored ${c.name}: new CIs of this class can be created again.`
      : `Archived ${c.name}: its CIs are kept, but no new ones can be created.`;
  else if (outcome.status === "refused") error.value = outcome.error;
}

async function purgeClass() {
  const c = cls.data.value!;
  error.value = null;
  saved.value = null;
  const outcome = await flow.run({
    title: `Purge class “${c.name}”?`,
    intro: `Deletes every CI of this class (deleted ones included) with their relationships, its attributes and relationship rules, and drops the table ${c.tableName} and the view ${c.viewName}.`,
    preview: { operation: "purgeType", id: c.id, body: { confirm: c.key } },
    apply: (confirm) => purge.mutateAsync({ id: c.id, confirm }),
    applyLabel: "Purge class and its CIs",
    danger: true,
    confirmName: c.key,
  });
  if (outcome.status === "applied") {
    flash.show("classes", `Purged class ${c.name}: table ${c.tableName} was dropped.`);
    await router.replace("/admin/classes");
  } else if (outcome.status === "refused") error.value = outcome.error;
}

const crumbs = computed(() => [
  { label: "Administration", to: "/admin" },
  { label: "CI classes", to: "/admin/classes" },
  { label: isNew.value ? "New" : (cls.data.value?.name ?? "…") },
]);
const notFound = computed(() => {
  const e = cls.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <LoadingState v-if="!isNew && cls.isLoading.value" label="Loading class…" />
  <template v-else-if="!isNew && cls.isError.value">
    <EmptyState v-if="notFound" title="CI class not found">
      No class has the id <code>{{ id }}</code>. It may have been deleted.
      <template #actions><RouterLink class="btn" to="/admin/classes">Back to CI classes</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="cls.error.value" :on-retry="() => cls.refetch()" />
  </template>
  <template v-else>
    <div class="page-header">
      <div class="title">
        <h1>
          <ClassBadge v-if="!isNew" :icon="cls.data.value?.icon" :color="cls.data.value?.color" />
          {{ isNew ? "New CI class" : cls.data.value?.name }}
        </h1>
        <span v-if="cls.data.value?.isAbstract" class="badge warn">Abstract</span>
        <span v-if="cls.data.value && !cls.data.value.isActive" class="badge off">Archived</span>
      </div>
      <div v-if="cls.data.value && !isNew" class="actions">
        <RouterLink class="btn" :to="`/cis?classId=${cls.data.value.id}`">Open inventory</RouterLink>
        <button v-if="cls.data.value.isActive" type="button" class="btn" :disabled="pending" @click="setActive(false)">Archive</button>
        <button v-else type="button" class="btn" :disabled="pending" @click="setActive(true)">Restore</button>
        <button v-if="!cls.data.value.isActive" type="button" class="btn btn-danger" :disabled="pending" @click="purgeClass">Purge…</button>
      </div>
    </div>
    <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>
    <div v-if="cls.data.value && !cls.data.value.isActive" class="alert alert-warn" role="note">
      This class is archived: its CIs and its table <code>{{ cls.data.value.tableName }}</code> are kept and still shown, but
      no new CIs can be created. Restore it to allow new CIs, or purge it to drop the table and delete its CIs.
    </div>
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />
    <div v-if="saved" class="alert" role="status">{{ saved }}</div>

    <form novalidate class="panel" aria-label="Class" @submit.prevent="submit">
      <div class="panel-header"><h2>Class</h2></div>
      <div class="panel-body form-grid">
        <FormField id="class-name" v-slot="p" label="Name" required :error="fieldErrors.name">
          <input :id="p.id" v-model="name" v-autofocus="isNew" type="text" maxlength="200" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField
          id="class-area"
          v-slot="p"
          label="Area"
          :required="isNew"
          :error="fieldErrors.areaId"
          :hint="isNew ? 'The menu tab and database schema its table goes into. Cannot change later.' : 'Fixed after creation'"
        >
          <select v-if="isNew" :id="p.id" v-model="areaId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" @change="areaTouched = true">
            <option value="" disabled>{{ areas.isLoading.value ? "Loading…" : activeAreas.length ? "Choose an area…" : "No areas yet" }}</option>
            <option v-for="a in activeAreas" :key="a.id" :value="a.id">{{ a.name }} ({{ a.key }})</option>
          </select>
          <input v-else :id="p.id" type="text" readonly :value="area ? `${area.name} (${area.key})` : ''" :aria-describedby="p.describedBy" />
          <span v-if="isNew && areas.data.value && activeAreas.length === 0" class="hint">
            <RouterLink to="/admin/areas">Create an area first</RouterLink>
          </span>
        </FormField>
        <TechnicalNameField
          id="class-key"
          v-model="key"
          kind="type"
          :name="name"
          :editable="isNew"
          :area-id="areaId"
          :location="cls.data.value?.tableName"
          :error="fieldErrors.key"
        />
        <FormField id="class-parent" v-slot="p" label="Parent class" :error="fieldErrors.parentId" hint="CIs of this class also carry the parent's attributes">
          <select :id="p.id" v-model="parentId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
            <option value="">— none (top level) —</option>
            <option v-for="n in parentOptions" :key="n.item.id" :value="n.item.id">
              {{ "  ".repeat(n.depth) }}{{ n.item.name }}{{ n.item.isActive ? "" : " (archived)" }}
            </option>
          </select>
        </FormField>
        <FormField id="class-icon" v-slot="p" label="Icon" :error="fieldErrors.icon">
          <div class="inline-control">
            <select :id="p.id" v-model="icon" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
              <option value="">— none —</option>
              <option v-for="i in CLASS_ICONS" :key="i.key" :value="i.key">{{ i.label }}</option>
              <option v-if="unknownIcon" :value="icon">{{ icon }} (custom)</option>
            </select>
            <ClassBadge :icon="icon" :color="color || '#56606d'" />
          </div>
        </FormField>
        <FormField id="class-color" v-slot="p" label="Colour" :error="fieldErrors.color" hint="Shown with the icon in menus and lists">
          <div class="inline-control">
            <input :id="p.id" type="color" :value="color || '#1f5fbf'" :aria-describedby="p.describedBy" @input="color = ($event.target as HTMLInputElement).value" />
            <span class="mono">{{ color || "none" }}</span>
            <button v-if="color" type="button" class="btn btn-sm" @click="color = ''">No colour</button>
          </div>
        </FormField>
        <div class="field">
          <span class="label">Kind</span>
          <label class="checkbox-row">
            <input id="class-abstract" v-model="isAbstract" type="checkbox" />
            Abstract: groups other classes, holds no CIs itself
          </label>
          <span v-if="fieldErrors.isAbstract" class="error">{{ fieldErrors.isAbstract }}</span>
        </div>
        <FormField id="class-description" v-slot="p" label="Description" wide :error="fieldErrors.description">
          <textarea :id="p.id" v-model="description" rows="2" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
      </div>
      <div class="form-footer">
        <button type="submit" class="btn btn-primary" :disabled="pending">{{ pending ? "Saving…" : isNew ? "Create class" : "Save class" }}</button>
        <RouterLink class="btn" to="/admin/classes">{{ isNew ? "Cancel" : "Back to classes" }}</RouterLink>
        <span v-if="cls.data.value && !isNew" class="muted" style="margin-left: auto; font-size: var(--fs-sm)">
          Created {{ formatDateTime(cls.data.value.createdAt) }} · updated {{ formatDateTime(cls.data.value.updatedAt) }}
        </span>
      </div>
    </form>

    <section v-if="cls.data.value && !isNew" class="panel" aria-labelledby="class-db-title">
      <div class="panel-header">
        <h2 id="class-db-title">In the database</h2>
        <span class="muted">For reporting tools and DBAs; the reporting view is read-only</span>
      </div>
      <div class="panel-body"><dl class="props">
        <dt>Table</dt>
        <dd><code>{{ cls.data.value.tableName }}</code> <span class="muted">(one row per CI, one typed column per attribute)</span></dd>
        <dt>Reporting view</dt>
        <dd><code>{{ cls.data.value.viewName }}</code> <span class="muted">(general CI fields plus every attribute, inherited ones included)</span></dd>
      </dl></div>
    </section>

    <AttributesEditor v-if="cls.data.value && !isNew" :cls="cls.data.value" />
  </template>
  <SchemaChangeDialog :flow="flow" />
</template>
