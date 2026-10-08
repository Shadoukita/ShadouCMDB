<script setup lang="ts">
import { t } from "../../../i18n";
import { adminCrumbs } from "../sections";
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useAreas, useCiClass, useCreateClass, usePatch, useRemove, type ClassCreateBody, type ClassUpdateBody } from "../../../api/datamodel";
import { usePurge } from "../../../api/schemaChanges";
import { useCiClasses, useClassAttributes, type CiClass } from "../../../api/queries";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ClassBadge from "../../../components/ClassBadge.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import Icon from "../../../components/Icon.vue";
import LoadingState from "../../../components/LoadingState.vue";
import RowMenu, { type RowMenuItem } from "../../../components/RowMenu.vue";
import SaveBar from "../../../components/SaveBar.vue";
import SchemaChangeDialog from "../../../components/SchemaChangeDialog.vue";
import TechnicalNameField from "../../../components/TechnicalNameField.vue";
import { CLASS_ICONS, classIcon } from "../../../lib/classIcons";
import { useDocumentTitle, useUnsavedGuard } from "../../../lib/composables";
import { vAutofocus } from "../../../lib/directives";
import { formatDateTime, formatRelative } from "../../../lib/format";
import { changedFields } from "../../../lib/changes";
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
 * abstract, icon, colour, title attribute and the data-quality owner and end-of-life fields; archive, restore or purge it. Every change is
 * previewed as DDL first. Title row, `⋯` menu and save bar as on the other admin
 * edit pages (design §2.7, audit A3). Below the form, the class's attribute editor
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
useDocumentTitle(() => (isNew.value ? t("dm.class.docTitleNew") : cls.data.value?.name));

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
/** The attribute whose value labels the class's CIs; "" labels them by their ident. */
const titleAttributeId = ref("");
/** Data quality: the fields holding a CI's owner and end of life; "" takes the parent's setting. */
const ownerAttributeId = ref("");
const endOfLifeAttributeId = ref("");
const error = ref<unknown>(null);
const local = ref<Record<string, string>>({});

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
  titleAttributeId.value = c?.titleAttributeId ?? "";
  ownerAttributeId.value = c?.ownerAttributeId ?? "";
  endOfLifeAttributeId.value = c?.endOfLifeAttributeId ?? "";
  initial = c ? formBody() : {};
  baseline.value = { id: c?.id, body: formBody() };
}
/** The form as loaded, to send only changed fields on save. */
let initial: ClassUpdateBody = {};
/** The form as seeded (a new class: as opened), for the save bar's count of changed fields. */
const baseline = ref<{ id: string | undefined; body: ClassUpdateBody } | null>(null);
function formBody(): ClassUpdateBody {
  return {
    name: name.value.trim(),
    description: description.value.trim() || null,
    parentId: parentId.value || null,
    isAbstract: isAbstract.value,
    icon: icon.value || null,
    color: color.value || null,
    titleAttributeId: titleAttributeId.value || null,
    ownerAttributeId: ownerAttributeId.value || null,
    endOfLifeAttributeId: endOfLifeAttributeId.value || null,
  };
}
const changes = computed(() => (baseline.value ? Object.keys(changedFields(formBody(), baseline.value.body)).length : 0));
const dirty = computed(() => changes.value > 0);
const guard = useUnsavedGuard(() => dirty.value, () => t("admin.unsaved.leave"));
// Seed from the record (a new class: from the query's area and parent), and again when it is
// refetched, unless that would overwrite unsaved edits.
watch(
  () => cls.data.value,
  (c) => {
    if (!baseline.value || !dirty.value || baseline.value.id !== c?.id) seed(c);
  },
  { immediate: true },
);
watch(id, () => {
  if (!id.value) seed(undefined);
  error.value = null;
  local.value = {};
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
/** Attributes that can label a CI: the class's own and inherited ones with a single readable value. */
const TITLE_TYPES = new Set(["text", "enum", "number", "integer", "date", "datetime", "ip", "cidr"]);
const attrs = useClassAttributes(id);
const titleOptions = computed(() => (attrs.data.value ?? []).filter((a) => TITLE_TYPES.has(a.dataType) && (a.isActive || a.id === titleAttributeId.value)));

/** Data quality (the dashboard's "Needs attention" checks): the field types each setting accepts, as the API's. */
const OWNER_TYPES = new Set(["text", "enum", "lookup", "reference"]);
const END_OF_LIFE_TYPES = new Set(["date", "datetime"]);
const ownerOptions = computed(() => (attrs.data.value ?? []).filter((a) => OWNER_TYPES.has(a.dataType) && (a.isActive || a.id === ownerAttributeId.value)));
const endOfLifeOptions = computed(() =>
  (attrs.data.value ?? []).filter((a) => END_OF_LIFE_TYPES.has(a.dataType) && (a.isActive || a.id === endOfLifeAttributeId.value)),
);
/**
 * What applies while the class has no setting of its own: the nearest ancestor's (of the parent chosen in the
 * form), with the field's label when this class inherits it; undefined when no ancestor sets one.
 */
function inheritedSetting(field: "ownerAttributeId" | "endOfLifeAttributeId") {
  const list = classes.data.value ?? [];
  const seen = new Set<string>();
  let c = list.find((x) => x.id === parentId.value);
  while (c && !seen.has(c.id)) {
    seen.add(c.id);
    const attrId = c[field];
    if (attrId) {
      const a = attrs.data.value?.find((x) => x.id === attrId);
      return { from: c.name, field: a ? `${a.label} (${a.key})` : t("dm.class.field.qualityUnknownField") };
    }
    c = list.find((x) => x.id === c!.parentId);
  }
  return undefined;
}
const ownerInherited = computed(() => inheritedSetting("ownerAttributeId"));
const endOfLifeInherited = computed(() => inheritedSetting("endOfLifeAttributeId"));
/** The empty choice: the parent's setting (named in full below the select), or the check left off for this class. */
function noneLabel(inherited: { from: string; field: string } | undefined) {
  return inherited ? t("dm.class.field.qualityInheritedShort", inherited) : t("dm.class.field.qualityNone");
}

const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const FIELDS = ["name", "key", "areaId", "description", "parentId", "isAbstract", "icon", "color", "titleAttributeId", "ownerAttributeId", "endOfLifeAttributeId"];
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field)) : []));

async function submit() {
  error.value = null;
  const errs: Record<string, string> = {};
  if (!name.value.trim()) errs.name = t("common.required");
  if (isNew.value) {
    const k = keyError(key.value);
    if (k) errs.key = k;
    // Without any area the API creates the default area "infrastruktur" for the class.
    if (!areaId.value && activeAreas.value.length > 0) errs.areaId = t("dm.class.areaRequired");
  }
  local.value = errs;
  if (Object.keys(errs).length > 0) {
    document.getElementById(errs.name ? "class-name" : errs.key ? "class-key" : "class-area")?.focus();
    return;
  }
  const body = formBody();
  // A new class has no attributes to be labelled by (or to check) yet.
  delete body.titleAttributeId;
  delete body.ownerAttributeId;
  delete body.endOfLifeAttributeId;
  if (isNew.value) {
    // New classes go to the end of the menu.
    const last = Math.max(0, ...(classes.data.value ?? []).map((c) => c.sortOrder));
    const createBody: ClassCreateBody = { ...body, name: body.name!, key: key.value, sortOrder: last + 10, ...(areaId.value ? { areaId: areaId.value } : {}) };
    const where = `${area.value?.key ?? "infrastruktur"}.${key.value}`;
    const outcome = await flow.run({
      title: t("dm.class.create.title", { name: createBody.name }),
      intro: t(body.isAbstract ? "dm.class.create.introAbstract" : "dm.class.create.intro", { table: where }),
      preview: { operation: "createType", body: createBody },
      apply: () => create.mutateAsync(createBody),
      applyLabel: t("dm.class.create"),
      alwaysShow: true,
    });
    if (outcome.status === "applied") {
      const created = outcome.result as CiClass;
      flash.show(t("dm.class.created", { name: created.name, table: created.tableName }));
      guard.allow();
      await router.push(`/admin/classes/${created.id}`);
    } else if (outcome.status === "refused") error.value = outcome.error;
    return;
  }
  // Only what changed (a changed title attribute relabels every CI of the class).
  const changed = changedFields(formBody(), initial);
  if (Object.keys(changed).length === 0) {
    flash.show(t("common.nothingChanged"));
    return;
  }
  const outcome = await flow.run({
    title: t("dm.class.save.title", { name: body.name ?? "" }),
    preview: { operation: "updateType", id: id.value!, body: changed },
    apply: () => update.mutateAsync({ id: id.value!, body: changed }),
    applyLabel: t("dm.class.save"),
  });
  if (outcome.status === "applied") {
    const next = outcome.result as CiClass;
    seed(next);
    flash.show(t("dm.class.saved", { name: next.name }));
  } else if (outcome.status === "refused") error.value = outcome.error;
}

async function setActive(isActive: boolean) {
  const c = cls.data.value!;
  error.value = null;
  const outcome = isActive
    ? await flow.run({
        title: t("dm.class.restore.title", { name: c.name }),
        preview: { operation: "updateType", id: c.id, body: { isActive: true } },
        apply: () => update.mutateAsync({ id: c.id, body: { isActive: true } }),
        applyLabel: t("dm.class.restore.apply"),
      })
    : await flow.run({
        title: t("dm.class.archive.title", { name: c.name }),
        intro: t("dm.class.archive.intro", { table: c.tableName }),
        preview: { operation: "deleteType", id: c.id },
        apply: () => remove.mutateAsync(c.id),
        applyLabel: t("dm.class.archive.apply"),
        alwaysShow: true,
      });
  if (outcome.status === "applied") flash.show(t(isActive ? "dm.class.restored" : "dm.class.archived", { name: c.name }));
  else if (outcome.status === "refused") error.value = outcome.error;
}

async function purgeClass() {
  const c = cls.data.value!;
  error.value = null;
  const outcome = await flow.run({
    title: t("dm.class.purge.title", { name: c.name }),
    intro: t("dm.class.purge.intro", { table: c.tableName, view: c.viewName }),
    preview: { operation: "purgeType", id: c.id, body: { confirm: c.key } },
    apply: (confirm) => purge.mutateAsync({ id: c.id, confirm }),
    applyLabel: t("dm.class.purge.apply"),
    danger: true,
    confirmName: c.key,
  });
  if (outcome.status === "applied") {
    flash.show(t("dm.class.purged", { name: c.name, table: c.tableName }));
    guard.allow();
    await router.replace("/admin/classes");
  } else if (outcome.status === "refused") error.value = outcome.error;
}

/** Back to the stored values. */
function discard() {
  seed(cls.data.value);
  error.value = null;
  local.value = {};
}

/** The `⋯` menu: archive or restore, and the purge of an archived class. */
const moreActions = computed<RowMenuItem[]>(() => {
  const c = cls.data.value;
  if (!c) return [];
  return c.isActive
    ? [{ label: t("dm.class.archive"), action: () => void setActive(false) }]
    : [
        { label: t("dm.class.restore"), action: () => void setActive(true) },
        { label: t("dm.class.purge"), danger: true, action: () => void purgeClass() },
      ];
});

const crumbs = computed(() => adminCrumbs("classes", { label: isNew.value ? t("admin.crumb.new") : (cls.data.value?.name ?? "…") }));
const notFound = computed(() => {
  const e = cls.error.value;
  return e instanceof ApiError && (e.code === "NOT_FOUND" || (e.code === "VALIDATION_ERROR" && e.details.some((d) => d.in === "params")));
});
</script>

<template>
  <Breadcrumbs :items="crumbs" />
  <LoadingState v-if="!isNew && cls.isLoading.value" :label="t('dm.class.loading')" />
  <template v-else-if="!isNew && cls.isError.value">
    <EmptyState v-if="notFound" icon="search" :title="t('dm.class.notFound.title')">
      {{ t("dm.class.notFound.body", { id: id ?? "" }) }}
      <template #actions><RouterLink class="btn" to="/admin/classes">{{ t("dm.class.back") }}</RouterLink></template>
    </EmptyState>
    <ErrorAlert v-else :error="cls.error.value" :on-retry="() => cls.refetch()" />
  </template>
  <template v-else>
    <div class="page-header record-header">
      <div class="record-heading">
        <div class="title">
          <ClassBadge v-if="!isNew && cls.data.value && (cls.data.value.icon || cls.data.value.color)" :icon="cls.data.value.icon" :color="cls.data.value.color" />
          <Icon v-else name="layers" class="class-icon" />
          <h1 dir="auto">{{ isNew ? t("dm.class.new") : cls.data.value?.name }}</h1>
        </div>
        <p v-if="cls.data.value && !isNew" class="record-meta" data-testid="record-meta">
          <span v-if="area">{{ area.name }}</span>
          <span v-if="area" class="sep" aria-hidden="true">·</span>
          <span class="ident">{{ cls.data.value.tableName }}</span>
          <template v-if="cls.data.value.isAbstract">
            <span class="sep" aria-hidden="true">·</span>
            <span class="badge warn" :title="t('dm.class.abstractTitle')">{{ t("dm.class.abstract") }}</span>
          </template>
          <template v-if="!cls.data.value.isActive">
            <span class="sep" aria-hidden="true">·</span>
            <span class="badge off">{{ t("dm.class.archivedBadge") }}</span>
          </template>
          <span class="sep" aria-hidden="true">·</span>
          <time :datetime="cls.data.value.updatedAt" :title="formatDateTime(cls.data.value.updatedAt)">
            {{ t("record.meta.updated", { when: formatRelative(cls.data.value.updatedAt) }) }}
          </time>
        </p>
      </div>
      <div v-if="cls.data.value && !isNew" class="actions">
        <RouterLink class="btn" :to="`/cis?classId=${cls.data.value.id}`">{{ t("dm.class.openInventory") }}</RouterLink>
        <RowMenu :label="t('record.actions.more')" :items="moreActions" large />
      </div>
    </div>
    <div v-if="cls.data.value && !cls.data.value.isActive" class="alert alert-warn" role="note">
      {{ t("dm.class.archivedNote", { table: cls.data.value.tableName }) }}
    </div>
    <FormErrorBanner v-if="error" :error="error" :unplaced="unplaced" />

    <form id="class-form" novalidate class="panel" :aria-label="t('dm.class.form')" @submit.prevent="submit">
      <div class="panel-header"><h2>{{ t("dm.class.form") }}</h2></div>
      <div class="panel-body form-grid">
        <FormField id="class-name" v-slot="p" :label="t('dm.class.field.name')" required :error="fieldErrors.name">
          <input :id="p.id" v-model="name" v-autofocus="isNew" type="text" maxlength="200" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
        <FormField
          id="class-area"
          v-slot="p"
          :label="t('dm.class.field.area')"
          :required="isNew"
          :error="fieldErrors.areaId"
          :hint="isNew ? t('dm.class.field.areaHint') : t('dm.class.field.areaFixed')"
        >
          <select v-if="isNew" :id="p.id" v-model="areaId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" @change="areaTouched = true">
            <option value="" :disabled="activeAreas.length > 0">
              {{ areas.isLoading.value ? t("common.loading") : activeAreas.length ? t("dm.class.field.areaChoose") : t("dm.class.field.areaDefault") }}
            </option>
            <option v-for="a in activeAreas" :key="a.id" :value="a.id">{{ a.name }} ({{ a.key }})</option>
          </select>
          <input v-else :id="p.id" type="text" readonly :value="area ? `${area.name} (${area.key})` : ''" :aria-describedby="p.describedBy" />
          <span v-if="isNew && areas.data.value && activeAreas.length === 0" class="hint">
            {{ t("dm.class.field.noAreas") }} <RouterLink to="/admin/areas">{{ t("dm.class.field.createArea") }}</RouterLink>
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
        <FormField id="class-parent" v-slot="p" :label="t('dm.class.field.parent')" :error="fieldErrors.parentId" :hint="t('dm.class.field.parentHint')">
          <select :id="p.id" v-model="parentId" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
            <option value="">{{ t("dm.class.field.parentNone") }}</option>
            <option v-for="n in parentOptions" :key="n.item.id" :value="n.item.id">
              {{ "  ".repeat(n.depth) }}{{ n.item.isActive ? n.item.name : t("dm.class.archivedName", { name: n.item.name }) }}
            </option>
          </select>
        </FormField>
        <FormField id="class-icon" v-slot="p" :label="t('dm.class.field.icon')" :error="fieldErrors.icon">
          <div class="inline-control">
            <select :id="p.id" v-model="icon" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
              <option value="">{{ t("dm.class.field.iconNone") }}</option>
              <option v-for="i in CLASS_ICONS" :key="i.key" :value="i.key">{{ i.label }}</option>
              <option v-if="unknownIcon" :value="icon">{{ t("dm.class.field.iconCustom", { icon }) }}</option>
            </select>
            <ClassBadge :icon="icon" :color="color || '#56606d'" />
          </div>
        </FormField>
        <FormField id="class-color" v-slot="p" :label="t('dm.class.field.color')" :error="fieldErrors.color" :hint="t('dm.class.field.colorHint')">
          <div class="inline-control">
            <input :id="p.id" type="color" :value="color || '#1f5fbf'" :aria-describedby="p.describedBy" @input="color = ($event.target as HTMLInputElement).value" />
            <span class="mono">{{ color || t("dm.class.field.colorUnset") }}</span>
            <button v-if="color" type="button" class="btn btn-sm" @click="color = ''">{{ t("dm.class.field.colorClear") }}</button>
          </div>
        </FormField>
        <div class="field">
          <span class="label">{{ t("dm.class.field.kind") }}</span>
          <label class="checkbox-row">
            <input id="class-abstract" v-model="isAbstract" type="checkbox" />
            {{ t("dm.class.field.abstract") }}
          </label>
          <span v-if="fieldErrors.isAbstract" class="error">{{ fieldErrors.isAbstract }}</span>
        </div>
        <FormField
          v-if="!isNew"
          id="class-title"
          v-slot="p"
          :label="t('dm.class.field.title')"
          :error="fieldErrors.titleAttributeId"
          :hint="t('dm.class.field.titleHint')"
        >
          <select :id="p.id" v-model="titleAttributeId" :disabled="attrs.isLoading.value" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
            <option value="">{{ t("dm.class.field.titleNone") }}</option>
            <option v-for="a in titleOptions" :key="a.id" :value="a.id">
              {{ a.label }} ({{ a.key }}){{ a.inherited ? ` · ${t("dm.class.field.titleFrom", { name: a.definedOn.name })}` : "" }}{{
                a.isActive ? "" : ` ${t("dm.class.field.titleRetired")}`
              }}
            </option>
            <option v-if="titleAttributeId && attrs.data.value && !titleOptions.some((a) => a.id === titleAttributeId)" :value="titleAttributeId">
              {{ t("dm.class.field.titleCurrent") }}
            </option>
          </select>
        </FormField>
        <FormField
          v-if="!isNew"
          id="class-owner"
          v-slot="p"
          :label="t('dm.class.field.owner')"
          :error="fieldErrors.ownerAttributeId"
          :hint="t('dm.class.field.ownerHint')"
        >
          <select :id="p.id" v-model="ownerAttributeId" :disabled="attrs.isLoading.value" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
            <option value="">{{ noneLabel(ownerInherited) }}</option>
            <option v-for="a in ownerOptions" :key="a.id" :value="a.id">
              {{ a.label }} ({{ a.key }}){{ a.inherited ? ` · ${t("dm.class.field.titleFrom", { name: a.definedOn.name })}` : "" }}{{
                a.isActive ? "" : ` ${t("dm.class.field.titleRetired")}`
              }}
            </option>
            <option v-if="ownerAttributeId && attrs.data.value && !ownerOptions.some((a) => a.id === ownerAttributeId)" :value="ownerAttributeId">
              {{ t("dm.class.field.titleCurrent") }}
            </option>
          </select>
          <span v-if="!ownerAttributeId && ownerInherited" class="hint" data-testid="owner-inherited">{{ t("dm.class.field.qualityInherited", ownerInherited) }}</span>
          <span v-if="attrs.data.value && ownerOptions.length === 0" class="hint">{{ t("dm.class.field.ownerNoFields") }}</span>
        </FormField>
        <FormField
          v-if="!isNew"
          id="class-end-of-life"
          v-slot="p"
          :label="t('dm.class.field.endOfLife')"
          :error="fieldErrors.endOfLifeAttributeId"
          :hint="t('dm.class.field.endOfLifeHint')"
        >
          <select :id="p.id" v-model="endOfLifeAttributeId" :disabled="attrs.isLoading.value" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy">
            <option value="">{{ noneLabel(endOfLifeInherited) }}</option>
            <option v-for="a in endOfLifeOptions" :key="a.id" :value="a.id">
              {{ a.label }} ({{ a.key }}){{ a.inherited ? ` · ${t("dm.class.field.titleFrom", { name: a.definedOn.name })}` : "" }}{{
                a.isActive ? "" : ` ${t("dm.class.field.titleRetired")}`
              }}
            </option>
            <option
              v-if="endOfLifeAttributeId && attrs.data.value && !endOfLifeOptions.some((a) => a.id === endOfLifeAttributeId)"
              :value="endOfLifeAttributeId"
            >
              {{ t("dm.class.field.titleCurrent") }}
            </option>
          </select>
          <span v-if="!endOfLifeAttributeId && endOfLifeInherited" class="hint" data-testid="end-of-life-inherited">{{ t("dm.class.field.qualityInherited", endOfLifeInherited) }}</span>
          <span v-if="attrs.data.value && endOfLifeOptions.length === 0" class="hint">{{ t("dm.class.field.endOfLifeNoFields") }}</span>
        </FormField>
        <FormField id="class-description" v-slot="p" :label="t('dm.class.field.description')" wide :error="fieldErrors.description">
          <textarea :id="p.id" v-model="description" rows="2" :aria-invalid="p.invalid || undefined" :aria-describedby="p.describedBy" />
        </FormField>
      </div>
    </form>

    <SaveBar :label="t('record.save.region')" :dirty="!isNew && dirty" :changes="isNew ? 0 : changes">
      <RouterLink class="btn" to="/admin/classes">{{ t("common.cancel") }}</RouterLink>
      <button v-if="!isNew && dirty" type="button" class="btn" :disabled="pending" @click="discard">{{ t("record.save.discard") }}</button>
      <button type="submit" form="class-form" class="btn btn-primary" :disabled="pending">
        {{ pending ? t("common.saving") : isNew ? t("dm.class.create") : t("dm.class.save") }}
      </button>
    </SaveBar>

    <section v-if="cls.data.value && !isNew" class="panel" aria-labelledby="class-db-title">
      <div class="panel-header">
        <h2 id="class-db-title">{{ t("dm.class.db.title") }}</h2>
        <span class="muted">{{ t("dm.class.db.subtitle") }}</span>
      </div>
      <div class="panel-body">
        <dl class="props">
          <dt>{{ t("dm.class.db.table") }}</dt>
          <dd><code>{{ cls.data.value.tableName }}</code> <span class="muted">{{ t("dm.class.db.tableNote") }}</span></dd>
          <dt>{{ t("dm.class.db.view") }}</dt>
          <dd><code>{{ cls.data.value.viewName }}</code> <span class="muted">{{ t("dm.class.db.viewNote") }}</span></dd>
          <dt>{{ t("common.created") }}</dt>
          <dd>{{ formatDateTime(cls.data.value.createdAt) }}</dd>
          <dt>{{ t("common.updated") }}</dt>
          <dd>{{ formatDateTime(cls.data.value.updatedAt) }}</dd>
        </dl>
      </div>
    </section>

    <AttributesEditor v-if="cls.data.value && !isNew" :cls="cls.data.value" />
  </template>
  <SchemaChangeDialog :flow="flow" />
</template>
