<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import { ApiError } from "../../../api/client";
import { useCiClass, useCreateClass, usePatch, type ClassUpdateBody } from "../../../api/datamodel";
import { useCiClasses, type CiClass } from "../../../api/queries";
import Breadcrumbs from "../../../components/Breadcrumbs.vue";
import ClassBadge from "../../../components/ClassBadge.vue";
import DeleteRowButton from "../../../components/DeleteRowButton.vue";
import EmptyState from "../../../components/EmptyState.vue";
import ErrorAlert from "../../../components/ErrorAlert.vue";
import LoadingState from "../../../components/LoadingState.vue";
import { CLASS_ICONS, classIcon } from "../../../lib/classIcons";
import { useDocumentTitle } from "../../../lib/composables";
import { vAutofocus } from "../../../lib/directives";
import { formatDateTime } from "../../../lib/format";
import { keyError, suggestKey } from "../../../lib/keys";
import { descendantIds, flattenTree } from "../../../lib/tree";
import { useFlashStore } from "../../../stores/flash";
import FormErrorBanner from "../../form/FormErrorBanner.vue";
import FormField from "../../form/FormField.vue";
import AttributesEditor from "./AttributesEditor.vue";

/**
 * Create or edit a CI class: name, key (fixed after creation), parent, abstract,
 * icon and colour; archive, restore or delete it. Below the form, the class's
 * attribute editor (existing classes only).
 */
const route = useRoute();
const router = useRouter();
const flash = useFlashStore();
const id = computed(() => (route.path.endsWith("/new") ? undefined : String(route.params.id ?? "")));
const isNew = computed(() => !id.value);
const cls = useCiClass(id);
const classes = useCiClasses();
const create = useCreateClass();
const update = usePatch<CiClass>("ci-classes");
const pending = computed(() => create.isPending.value || update.isPending.value);
const flashText = computed(() => (id.value ? flash.forCi(id.value) : undefined));
useDocumentTitle(() => (isNew.value ? "New class" : cls.data.value?.name));

const name = ref("");
const key = ref("");
const keyTouched = ref(false);
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
  keyTouched.value = !!c;
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
watch(name, (n) => {
  if (isNew.value && !keyTouched.value) key.value = suggestKey(n);
});

/** Parent choices: every class except this one and its subclasses (that would be a cycle), indented as a tree. */
const parentOptions = computed(() => {
  const list = classes.data.value ?? [];
  const excluded = id.value ? new Set([id.value, ...descendantIds(list, id.value)]) : new Set<string>();
  return flattenTree(list).filter((n) => !excluded.has(n.item.id));
});
const unknownIcon = computed(() => !!icon.value && !classIcon(icon.value));

const fieldErrors = computed(() => ({ ...(error.value instanceof ApiError ? error.value.fieldErrors() : {}), ...local.value }));
const FIELDS = ["name", "key", "description", "parentId", "isAbstract", "icon", "color"];
const unplaced = computed(() => (error.value instanceof ApiError ? error.value.details.filter((d) => !FIELDS.includes(d.field)) : []));

async function submit() {
  error.value = null;
  saved.value = null;
  const errs: Record<string, string> = {};
  if (!name.value.trim()) errs.name = "Required";
  if (isNew.value) {
    const k = keyError(key.value);
    if (k) errs.key = k;
  }
  local.value = errs;
  if (Object.keys(errs).length > 0) {
    document.getElementById(errs.name ? "class-name" : "class-key")?.focus();
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
  try {
    if (isNew.value) {
      // New classes go to the end of the menu.
      const last = Math.max(0, ...(classes.data.value ?? []).map((c) => c.sortOrder));
      const created = await create.mutateAsync({ ...body, name: body.name!, key: key.value, sortOrder: last + 10 });
      flash.show(created.id, `Created class ${created.name}. Add its attributes below.`);
      await router.push(`/admin/classes/${created.id}`);
      return;
    }
    const next = await update.mutateAsync({ id: id.value!, body });
    saved.value = `Saved ${next.name}.`;
  } catch (e) {
    error.value = e;
  }
}

async function setActive(isActive: boolean) {
  error.value = null;
  saved.value = null;
  try {
    const next = await update.mutateAsync({ id: id.value!, body: { isActive } });
    saved.value = isActive
      ? `Restored ${next.name}: new CIs of this class can be created again.`
      : `Archived ${next.name}: its CIs are kept, but no new ones can be created.`;
  } catch (e) {
    error.value = e;
  }
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
        <DeleteRowButton
          resource="ci-classes"
          :id="cls.data.value.id"
          :label="`class “${cls.data.value.name}”`"
          archivable
          :archived="!cls.data.value.isActive"
          @archive="setActive(false)"
          @deleted="router.replace('/admin/classes')"
        />
      </div>
    </div>
    <div v-if="flashText" class="alert" role="status">{{ flashText }}</div>
    <div v-if="cls.data.value && !cls.data.value.isActive" class="alert alert-warn" role="note">
      This class is archived: its CIs are kept and still shown, but no new ones can be created. Restore it to allow new CIs.
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
          id="class-key"
          v-slot="p"
          label="Key"
          :required="isNew"
          :error="fieldErrors.key"
          :hint="isNew ? 'Used by imports and the API. Cannot change later.' : 'Fixed after creation'"
        >
          <input
            :id="p.id"
            v-model="key"
            type="text"
            class="mono"
            spellcheck="false"
            :readonly="!isNew"
            :aria-invalid="p.invalid || undefined"
            :aria-describedby="p.describedBy"
            @input="keyTouched = true"
          />
        </FormField>
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

    <AttributesEditor v-if="cls.data.value && !isNew" :cls="cls.data.value" />
  </template>
</template>
